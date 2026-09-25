-- declines: groups by a key
SELECT mk.keyword_id, MIN(t.title) AS movie_title FROM title AS t, movie_keyword AS mk WHERE t.id = mk.movie_id GROUP BY mk.keyword_id ORDER BY mk.keyword_id NULLS LAST;
